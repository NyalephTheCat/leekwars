import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4959 extends AI {
public AI_4959() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
session.rebindAll(this);
MapLeekValue u_m = (MapLeekValue) ops(toMap(0, new MapLeekValue(AI_4959.this)), 1);
session.setVariable(AI_4959.this, "m", u_m);
return null;
}
protected String getAIString() { return "<snippet 4959>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4959>", };}

protected int[] getErrorFilesID() { return new int[] {4959, };}

}
