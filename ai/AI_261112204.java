import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_261112204 extends AI {
public AI_261112204() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
private Object f_u() throws LeekRunException {
ops(1);return 2l;
}
public Object runIA(Session session) throws LeekRunException {
return f_u();
}
protected String getAIString() { return "Main_1a99d49586317_38";}
protected String[] getErrorFiles() { return new String[] {"Class/util", "Main_1a99d49586317_38", };}

protected int[] getErrorFilesID() { return new int[] {48275994, 261112204, };}

}
