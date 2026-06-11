import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_5347 extends AI {
public AI_5347() throws LeekRunException {
super(1, 3);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
ops(4); return new SetLeekValue(AI_5347.this, new Object[] { 1l, 2l });
}
protected String getAIString() { return "<snippet 5347>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 5347>", };}

protected int[] getErrorFilesID() { return new int[] {5347, };}

}
